//! The app's own library files, `albums.json` and `presets.json`: each is
//! held by a `JsonFileStore`, read once per run, and every change to it is one
//! read-modify-write under the store's write lock, persisted with
//! `rawtonic_fs::atomic_write` (the same file, format and helper as before the
//! store) before readers see it. `AlbumsStore` and `PresetsStore` are the two
//! files.
//!
//! Only this process writes them while the app runs: the GUI is a single
//! instance (`tauri_plugin_single_instance`), and the headless runs of the
//! same binary never write albums or presets. Changes come from the
//! frontend's commands and from the file operations' album sync, all on
//! blocking workers; they queue on the store's write lock, so two changes
//! never read the same file and write over one another, and the frontend
//! sends its own saves in order (`src/store/libraryFiles.ts`).
//!
//! A file that is there but cannot be read stays untouched: every change
//! fails, as it did when each change read the file itself, and it is read
//! again on the next access. A file that is not there yet is an empty list;
//! changes are written over it as before. A hand edit of the file while the
//! app runs was always overwritten by the next save; it now also stays unseen
//! until the next start.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tauri::AppHandle;

use crate::file_management::{AlbumItem, PresetItem, sort_album_tree};
use crate::state::lock::{lock_recover, read_recover, write_recover};

struct JsonFileStore<T> {
    /// `None` until the file was read (or found missing); a read that failed
    /// is tried again on the next access.
    current: RwLock<Option<Arc<Vec<T>>>>,
    /// Held for a whole load or read-modify-write (lock order: `writer`,
    /// then `current`).
    writer: Mutex<()>,
}

impl<T> JsonFileStore<T> {
    /// A store that has read nothing yet: the app's, and a test's own.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> Default for JsonFileStore<T> {
    fn default() -> Self {
        Self {
            current: RwLock::new(None),
            writer: Mutex::new(()),
        }
    }
}

/// The app's album list (`albums.json`), kept sorted as it is saved.
pub struct AlbumsStore(JsonFileStore<AlbumItem>);

/// The user's presets (`presets.json`).
pub struct PresetsStore(JsonFileStore<PresetItem>);

impl Default for AlbumsStore {
    fn default() -> Self {
        Self(JsonFileStore::new())
    }
}

impl Default for PresetsStore {
    fn default() -> Self {
        Self(JsonFileStore::new())
    }
}

macro_rules! json_file_store {
    ($store:ty, $path:path, $body:ty, $read:ident, $sort:ident) => {
        impl $store {
            /// The stored items, read from the file on the first call of the
            /// run (empty when it is not there). `parse` is how the file's
            /// text is read, `sort` how it is kept.
            pub fn get(&self, app_handle: &AppHandle) -> Result<Arc<Vec<$body>>, String> {
                self.0
                    .get(|| $path(app_handle), $read, &|items| $sort(items))
            }

            /// Applies `change` to the stored items and saves the result, all
            /// under the write lock, so concurrent changes never lose one
            /// another. `change` returns what to save, or `None` to save
            /// nothing. On an error neither the file nor the items change.
            pub fn update<R>(
                &self,
                app_handle: &AppHandle,
                change: impl FnOnce(&mut Vec<$body>) -> Option<R>,
            ) -> Result<Option<R>, String> {
                self.0
                    .update(|| $path(app_handle), $read, &|items| $sort(items), change)
            }
        }
    };
}

fn read_albums(text: &str) -> Result<Vec<AlbumItem>, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn read_presets(text: &str) -> Result<Vec<PresetItem>, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn no_order<T>(_: &mut Vec<T>) {}

json_file_store!(
    AlbumsStore,
    crate::file_management::get_albums_path,
    AlbumItem,
    read_albums,
    sort_album_tree
);
json_file_store!(
    PresetsStore,
    crate::file_management::get_presets_path,
    PresetItem,
    read_presets,
    no_order
);

impl<T: serde::Serialize + Clone> JsonFileStore<T> {
    /// Call with no lock held.
    fn get(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
        parse: impl FnOnce(&str) -> Result<Vec<T>, String>,
        sort: &dyn Fn(&mut Vec<T>),
    ) -> Result<Arc<Vec<T>>, String> {
        if let Some(items) = read_recover(&self.current).as_ref() {
            return Ok(items.clone());
        }
        let _writer = lock_recover(&self.writer);
        self.loaded(&path()?, parse, &sort)
    }

    /// Call with no lock held; `change` runs under the write lock.
    fn update<R>(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
        parse: impl FnOnce(&str) -> Result<Vec<T>, String>,
        sort: &dyn Fn(&mut Vec<T>),
        change: impl FnOnce(&mut Vec<T>) -> Option<R>,
    ) -> Result<Option<R>, String> {
        let _writer = lock_recover(&self.writer);
        let path = path()?;
        let mut next = (*self.loaded(&path, parse, sort)?).clone();
        let Some(result) = change(&mut next) else {
            return Ok(None);
        };
        sort(&mut next);
        let json = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        rawtonic_fs::atomic_write(&path, json).map_err(|e| e.to_string())?;
        *write_recover(&self.current) = Some(Arc::new(next));
        Ok(Some(result))
    }

    /// Call with `writer` held.
    fn loaded(
        &self,
        path: &PathBuf,
        parse: impl FnOnce(&str) -> Result<Vec<T>, String>,
        sort: &dyn Fn(&mut Vec<T>),
    ) -> Result<Arc<Vec<T>>, String> {
        if let Some(items) = read_recover(&self.current).as_ref() {
            return Ok(items.clone());
        }
        let mut items = if path.exists() {
            parse(&fs::read_to_string(path).map_err(|e| e.to_string())?)
        } else {
            Ok(Vec::new())
        }?;
        sort(&mut items);
        let items = Arc::new(items);
        *write_recover(&self.current) = Some(items.clone());
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn at(path: &Path) -> impl FnOnce() -> Result<PathBuf, String> + '_ {
        || Ok(path.to_path_buf())
    }

    fn read<T: DeserializeOwned>(path: &Path) -> Vec<T> {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn albums(names: &[&str]) -> Vec<AlbumItem> {
        names
            .iter()
            .map(|name| AlbumItem::Album {
                id: name.to_lowercase(),
                name: name.to_string(),
                icon: None,
                images: vec![format!("/photos/{name}.dng")],
            })
            .collect()
    }

    fn presets(ids: &[&str]) -> Vec<PresetItem> {
        ids.iter()
            .map(|id| {
                PresetItem::Preset(crate::file_management::Preset {
                    id: id.to_string(),
                    name: id.to_string(),
                    adjustments: serde_json::json!({ "exposure": 1 }),
                    include_masks: None,
                    include_crop_transform: None,
                    preset_type: None,
                    tool_keys: None,
                })
            })
            .collect()
    }

    fn album_names(items: &[AlbumItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                AlbumItem::Album { name, .. } => name.clone(),
                AlbumItem::Group { name, .. } => name.clone(),
            })
            .collect()
    }

    fn preset_ids(items: &[PresetItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                PresetItem::Preset(p) => p.id.clone(),
                PresetItem::Folder(f) => f.id.clone(),
            })
            .collect()
    }

    /// An album list already on disk, and a store over it.
    fn albums_on_disk(path: &Path) -> JsonFileStore<AlbumItem> {
        fs::write(path, serde_json::to_string(&albums(&["Base"])).unwrap()).unwrap();
        JsonFileStore::new()
    }

    /// A whole-list save (the frontend's) followed by a change to one album
    /// (a file operation's rename, the Albums tool's add): the change works
    /// on the saved list, so neither is lost.
    #[test]
    fn a_save_then_a_change_to_one_album_keep_both() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, serde_json::to_string(&albums(&["Shared"])).unwrap()).unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        let mut renamed = albums(&["Shared"]);
        if let AlbumItem::Album { name, .. } = &mut renamed[0] {
            name.push_str(" (renamed)");
        }
        store
            .update(at(&path), read_albums, &sort, move |items| {
                *items = renamed;
                Some(())
            })
            .unwrap();
        store
            .update(at(&path), read_albums, &sort, |items| {
                items.extend(albums(&["Added"]));
                Some(())
            })
            .unwrap();
        assert_eq!(
            album_names(&read::<AlbumItem>(&path)),
            ["Added", "Shared (renamed)"]
        );
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["Added", "Shared (renamed)"]
        );
    }

    /// The album list's order, for the store's generic calls.
    #[allow(clippy::ptr_arg)]
    fn sort(items: &mut Vec<AlbumItem>) {
        sort_album_tree(items);
    }

    #[test]
    fn changes_from_many_threads_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        fs::write(&path, serde_json::to_string(&presets(&["base"])).unwrap()).unwrap();
        let store = Arc::new(JsonFileStore::<PresetItem>::new());
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let (store, path) = (store.clone(), path.clone());
                std::thread::spawn(move || {
                    for i in 0..10 {
                        store
                            .update(at(&path), read_presets, &no_order, |items| {
                                items.extend(presets(&[&format!("t{t}-{i}")]));
                                Some(())
                            })
                            .unwrap();
                        // Readers meanwhile see whole lists, never a half change.
                        let seen = store.get(at(&path), read_presets, &no_order).unwrap();
                        assert!(seen.iter().any(|item| matches!(item, PresetItem::Preset(p) if p.id == format!("t{t}-{i}"))));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let ids = preset_ids(&read::<PresetItem>(&path));
        assert_eq!(ids.len(), 81);
        assert_eq!(
            preset_ids(&store.get(at(&path), read_presets, &no_order).unwrap()).len(),
            81
        );
    }

    #[test]
    fn a_change_that_changes_nothing_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        let store = albums_on_disk(&path);
        store.get(at(&path), read_albums, &sort).unwrap();
        fs::write(&path, b"marker").unwrap();
        let none = store
            .update(at(&path), read_albums, &sort, |_| None::<()>)
            .unwrap();
        assert!(none.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"marker");
        // The store kept what it had read, not the marker.
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["Base"]
        );
    }

    #[test]
    fn a_broken_file_stays_as_it_is_until_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, b"{\"not\": a list").unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        let change = |store: &JsonFileStore<AlbumItem>, name: &str| {
            store
                .update(
                    at(&path),
                    read_albums,
                    &|items: &mut Vec<AlbumItem>| sort_album_tree(items),
                    |items| {
                        items.extend(albums(&[name]));
                        Some(())
                    },
                )
                .map(|r| r.is_some())
        };
        assert!(change(&store, "First").is_err());
        assert!(
            store
                .get(at(&path), read_albums, &|items: &mut Vec<AlbumItem>| {
                    sort_album_tree(items)
                })
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"{\"not\": a list");
        fs::write(&path, serde_json::to_string(&albums(&["Base"])).unwrap()).unwrap();
        assert!(change(&store, "Late").unwrap());
        let mut names = album_names(&read::<AlbumItem>(&path));
        names.sort();
        assert_eq!(names, ["Base", "Late"]);
    }

    #[test]
    fn a_failed_write_changes_neither_the_file_nor_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        let store = albums_on_disk(&path);
        store.get(at(&path), read_albums, &sort).unwrap();
        // A directory in the file's place: the atomic write cannot replace it.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            store
                .update(at(&path), read_albums, &sort, |items| {
                    items.extend(albums(&["Lost"]));
                    Some(())
                })
                .is_err()
        );
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["Base"]
        );
        fs::remove_dir(&path).unwrap();
    }

    #[test]
    fn the_file_is_read_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        fs::write(&path, serde_json::to_string(&presets(&["a"])).unwrap()).unwrap();
        let reads = AtomicUsize::new(0);
        let store = JsonFileStore::<PresetItem>::new();
        let read_with_count = |text: &str| {
            reads.fetch_add(1, Ordering::Relaxed);
            read_presets(text)
        };
        for _ in 0..20 {
            assert_eq!(
                preset_ids(&store.get(at(&path), read_with_count, &no_order).unwrap()),
                ["a"]
            );
        }
        fs::write(&path, serde_json::to_string(&presets(&["a", "b"])).unwrap()).unwrap();
        assert_eq!(
            preset_ids(&store.get(at(&path), read_with_count, &no_order).unwrap()),
            ["a"]
        );
        store
            .update(at(&path), read_with_count, &no_order, |items| {
                items.extend(presets(&["c"]));
                Some(())
            })
            .unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 1, "one read per run");
        // The file changed behind the store is replaced by the store's list
        // plus the change: a hand edit while the app runs was always lost at
        // the next save.
        assert_eq!(preset_ids(&read::<PresetItem>(&path)), ["a", "c"]);
    }

    #[test]
    fn a_missing_file_is_an_empty_list_and_the_first_change_makes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        let store = JsonFileStore::<AlbumItem>::new();
        assert!(
            store
                .get(at(&path), read_albums, &|items: &mut Vec<AlbumItem>| {
                    sort_album_tree(items)
                })
                .unwrap()
                .is_empty()
        );
        store
            .update(
                at(&path),
                read_albums,
                &|items: &mut Vec<AlbumItem>| sort_album_tree(items),
                |items| {
                    items.extend(albums(&["New"]));
                    Some(())
                },
            )
            .unwrap();
        assert_eq!(album_names(&read::<AlbumItem>(&path)), ["New"]);
    }

    #[test]
    fn the_album_list_is_kept_sorted_as_it_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(
            &path,
            serde_json::to_string(&albums(&["Zebra", "alpha"])).unwrap(),
        )
        .unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        let items = store
            .get(at(&path), read_albums, &|items: &mut Vec<AlbumItem>| {
                sort_album_tree(items)
            })
            .unwrap();
        assert_eq!(album_names(&items), ["alpha", "Zebra"]);
        store
            .update(
                at(&path),
                read_albums,
                &|items: &mut Vec<AlbumItem>| sort_album_tree(items),
                |items| {
                    items.extend(albums(&["Middle"]));
                    Some(())
                },
            )
            .unwrap();
        assert_eq!(
            album_names(&read::<AlbumItem>(&path)),
            ["alpha", "Middle", "Zebra"]
        );
        // Written as the file always was: pretty JSON of the list.
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("[\n  {"));
    }
}
